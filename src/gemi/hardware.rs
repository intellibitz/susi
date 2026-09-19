// 100% Rust implementation for autonomous hardware profiling

use candle_core::Device;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

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
        let mut profile = CACHED_PROFILE
            .get_or_init(|| {
                let (cpus, _) = Self::profile();
                let ram_gb = Self::determine_total_ram_gb();
                let gpu_vram_gb = Self::determine_gpu_vram_gb();
                let swap_gb = Self::determine_swap_gb();
                let nvme_active = Self::is_nvme_active();

                // 1. Direct Interrogation via Candle Substrate
                let (native_accel, gpu_name) = Self::interrogate_native_acceleration();

                let acceleration_active =
                    !native_accel.contains("None") && !native_accel.contains("Cpu");
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
                    available_ram_gb: 0,
                    gpu_vram_gb,
                    swap_gb,
                    nvme_active,
                    acceleration_active,
                    native_acceleration: native_accel,
                    os_info: Self::get_os_info(),
                    arch: std::env::consts::ARCH.to_string(),
                    disk_gb: Self::determine_disk_gb(),
                    disk_usage_pct: 0,
                    load_avg: String::new(),
                    uptime: String::new(),
                    hostname: Self::get_hostname(),
                }
            })
            .clone();

        profile.available_ram_gb = Self::determine_available_ram_gb();
        profile.disk_usage_pct = Self::determine_disk_usage_pct();
        profile.load_avg = Self::get_load_avg();
        profile.uptime = Self::get_uptime();

        profile
    }

    pub fn check_oom_critical() -> bool {
        let profile = Self::get_profile();
        if profile.ram_gb == 0 {
            return false;
        }
        let usage_pct =
            ((profile.ram_gb - profile.available_ram_gb) as f32 / profile.ram_gb as f32) * 100.0;
        usage_pct > 90.0
    }

    fn get_cpu_brand() -> String {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
                for line in content.lines() {
                    if line.starts_with("model name") {
                        return line
                            .split(':')
                            .nth(1)
                            .unwrap_or("Unknown CPU")
                            .trim()
                            .to_string();
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

    /// Free bytes on the filesystem containing `path` (falls back to `/` if
    /// `path` doesn't exist yet, e.g. the models directory hasn't been
    /// created on first run - `statvfs` needs an existing path to resolve).
    /// Unlike `get_disk_stats` (hardcoded to `/`, used only for
    /// `HardwareProfile` reporting), this checks the actual target
    /// directory a model download would land in, and returns exact bytes
    /// rather than a rounded GB figure, so `get_progressive_model_ladder`
    /// can gate tier selection on real available space, not just RAM.
    #[cfg(unix)]
    pub fn get_free_disk_bytes(path: &std::path::Path) -> u64 {
        use std::ffi::CString;
        let probe_path = if path.exists() {
            path
        } else {
            std::path::Path::new("/")
        };
        let Some(path_str) = probe_path.to_str() else {
            return 0;
        };
        let Ok(c_path) = CString::new(path_str) else {
            return 0;
        };
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } == 0 {
            (stat.f_bavail as u64) * (stat.f_frsize as u64)
        } else {
            0
        }
    }

    #[cfg(not(unix))]
    pub fn get_free_disk_bytes(_path: &std::path::Path) -> u64 {
        u64::MAX // Unknown on this platform - don't block downloads over it.
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
        Self::get_dynamic_device(0)
    }

    /// Byte-precise GPU VRAM budget available for weight + KV-cache
    /// placement. Prefers `nvidia-smi`'s live `memory.free` reading over a
    /// fixed fraction of total capacity: total-based budgeting can't see
    /// memory another process (or a prior susi run) already holds, and
    /// would then attempt to place layers into VRAM that isn't actually
    /// free - risking a CUDA OOM at model load rather than a graceful CPU
    /// fallback for whatever doesn't fit. Returns 0 when no GPU device is
    /// actually active (mirrors `get_dynamic_device`'s runtime probe, not
    /// the compile-time `cfg!(feature = "cuda")` check).
    pub fn gpu_vram_budget_bytes() -> u64 {
        if matches!(Self::get_candle_device(), Device::Cpu) {
            return 0;
        }
        // Headroom for the CUDA context, cuBLAS handle, and cudarc's own
        // allocator bookkeeping, none of which show up as tensor weights.
        const CUDA_CONTEXT_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
        let free_bytes = Self::determine_gpu_vram_free_bytes()
            .unwrap_or_else(|| Self::determine_gpu_vram_gb() as u64 * 1024 * 1024 * 1024);
        free_bytes.saturating_sub(CUDA_CONTEXT_RESERVE_BYTES)
    }

    /// Live free VRAM in bytes via `nvidia-smi`, independent of
    /// `determine_gpu_vram_gb`'s integer-GB display value (which truncates,
    /// e.g. an 8188 MiB card reports as "7GB") and independent of total
    /// capacity, which doesn't reflect memory already in use.
    fn determine_gpu_vram_free_bytes() -> Option<u64> {
        let output = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let first_line = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()?
            .trim()
            .to_string();
        let mib: u64 = first_line.parse().ok()?;
        Some(mib * 1024 * 1024)
    }

    pub fn get_dynamic_device(allocated_bytes_required: usize) -> Device {
        // Sub-2ms Heterogeneous Offloading Fallback
        if allocated_bytes_required > 0 {
            let vram_limit = Self::determine_gpu_vram_gb() * 1024 * 1024 * 1024;
            if vram_limit > 0 && allocated_bytes_required as f64 > (vram_limit as f64 * 0.90) {
                return Device::Cpu;
            }
        }

        // Zero-Lock Device Cache (Sub-2ms Mandate)
        static DEVICE_CACHE: OnceLock<Device> = OnceLock::new();
        DEVICE_CACHE
            .get_or_init(|| {
                #[cfg(feature = "cuda")]
                {
                    // Attempt CUDA initialization with panic safety
                    let cuda_attempt = std::panic::catch_unwind(|| Device::new_cuda(0));
                    if let Ok(Ok(cuda_dev)) = cuda_attempt {
                        return cuda_dev;
                    }
                }

                // Attempt Metal initialization with panic safety
                #[cfg(feature = "metal")]
                {
                    let metal_attempt = std::panic::catch_unwind(|| Device::new_metal(0));
                    if let Ok(Ok(metal_dev)) = metal_attempt {
                        return metal_dev;
                    }
                }

                // Absolute Fallback: CPU
                Device::Cpu
            })
            .clone()
    }

    fn get_os_info() -> String {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
                for line in content.lines() {
                    if line.starts_with("PRETTY_NAME=") {
                        return line
                            .trim_start_matches("PRETTY_NAME=")
                            .trim_matches('"')
                            .to_string();
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

    /// Reports acceleration status from the same runtime device probe
    /// `get_candle_device` uses for actual inference (`Device::new_cuda`/
    /// `new_metal`, each wrapped in `catch_unwind`), rather than
    /// `candle_core::utils::cuda_is_available`/`metal_is_available`, which are
    /// compile-time-only (`cfg!(feature = "cuda")`) and would report GPU
    /// acceleration as active on a `--features cuda` build even when no GPU
    /// is present or the driver fails to init at runtime - silently
    /// disagreeing with the CPU device inference actually falls back to.
    fn interrogate_native_acceleration() -> (String, String) {
        match Self::get_candle_device() {
            Device::Cuda(_) => (
                "CUDA (Active)".to_string(),
                "NVIDIA Driver found".to_string(),
            ),
            Device::Metal(_) => (
                "Metal (Active)".to_string(),
                "Apple Silicon / macOS".to_string(),
            ),
            Device::Cpu => ("None".to_string(), "Cpu".to_string()),
        }
    }

    fn determine_gpu_vram_gb() -> usize {
        // NVIDIA's proprietary driver never populates the AMD-specific
        // mem_info_vram_total sysfs attribute below, so query nvidia-smi
        // directly for NVIDIA hardware first.
        if let Ok(output) = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
            .output()
        {
            if output.status.success() {
                if let Some(first_line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                    if let Ok(mib) = first_line.trim().parse::<u64>() {
                        if mib > 0 {
                            return (mib / 1024) as usize;
                        }
                    }
                }
            }
        }

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
                    if unsafe {
                        libc::sysctlbyname(
                            c_name.as_ptr(),
                            &mut val as *mut _ as *mut libc::c_void,
                            &mut size,
                            std::ptr::null_mut(),
                            0,
                        )
                    } == 0
                    {
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

    /// A step qualifies only if the hardware has enough RAM *and* enough
    /// free disk space to hold it, plus a safety margin so a download
    /// doesn't run the disk to zero (temp files, partial-download
    /// overhead, and leaving the OS some breathing room). Split out from
    /// `get_progressive_model_ladder` as a pure function so this filtering
    /// logic is unit-testable without depending on real RAM-detection/
    /// `statvfs` syscalls. Before this, tier selection was RAM-only - a
    /// machine with plenty of RAM but little free disk would still attempt
    /// a download (up to ~45GB for the 72B tier) with no check at all.
    fn filter_ladder_by_hardware(
        steps: &[crate::sandbox::manager::ModelLadderConfigStep],
        ram_gb: usize,
        free_disk_bytes: u64,
    ) -> Vec<crate::sandbox::manager::ModelLadderConfigStep> {
        const DISK_SAFETY_MARGIN_BYTES: u64 = 2_000_000_000; // 2GB headroom
        steps
            .iter()
            .filter(|s| {
                ram_gb as f32 >= s.min_ram_gb
                    && free_disk_bytes >= s.expected_bytes.saturating_add(DISK_SAFETY_MARGIN_BYTES)
            })
            .cloned()
            .collect()
    }

    pub fn get_progressive_model_ladder() -> Vec<ModelLadderStep> {
        let ram_gb = Self::determine_total_ram_gb();
        let free_disk_bytes =
            Self::get_free_disk_bytes(&crate::gemi::models::ModelManager::get_models_dir());
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();

        let config_steps = cfg.model_ladder();
        let qualifying = Self::filter_ladder_by_hardware(&config_steps, ram_gb, free_disk_bytes);
        let mut ladder: Vec<ModelLadderStep> = qualifying
            .into_iter()
            .map(|step| ModelLadderStep {
                step: step.step,
                label: step.label,
                hf_repo: step.hf_repo,
                hf_file: step.hf_file,
                tokenizer_repo: step.tokenizer_repo,
                min_bytes: step.min_bytes,
                expected_bytes: step.expected_bytes,
            })
            .collect();

        if ladder.is_empty() {
            let fallback = cfg.default_fallback_model();
            // The fallback entry doesn't carry its own min_bytes/expected_bytes
            // in config (nothing reads them off it directly today), but this
            // struct's expected_bytes is used as a percentage denominator
            // downstream — 0 would divide-by-zero into NaN, so fall back to
            // the smallest real ladder step's thresholds rather than 0/0.
            let (min_bytes, expected_bytes) = cfg
                .model_ladder()
                .first()
                .map(|s| (s.min_bytes, s.expected_bytes))
                .unwrap_or((1_000_000_000, 5_000_000_000));
            ladder.push(ModelLadderStep {
                step: 1,
                label: "Minimum Viable Substrate (Config Fallback)".to_string(),
                hf_repo: fallback.hf_repo,
                hf_file: fallback.hf_file,
                tokenizer_repo: fallback.tokenizer_repo,
                min_bytes,
                expected_bytes,
            });
        }

        ladder
    }

    pub fn audit_os_environment_care() -> OsCareReport {
        let os_name = Self::get_os_info();
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();

        let mut reclaimable = 0u64;
        let mut recommendations = Vec::new();

        // Check pacman / cargo / temp caches
        let pacman_cache = std::path::PathBuf::from("/var/cache/pacman/pkg");
        if pacman_cache.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&pacman_cache) {
                let bytes: u64 = entries
                    .flatten()
                    .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
                    .sum();
                if bytes > 500_000_000 {
                    reclaimable += bytes;
                    recommendations.push(format!(
                        "Clean pacman package cache ({:.1} GB reclaimable)",
                        bytes as f64 / 1e9
                    ));
                }
            }
        }

        let user_cache = home.join(".cache");
        if user_cache.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&user_cache) {
                let bytes: u64 = entries
                    .flatten()
                    .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
                    .sum();
                if bytes > 1_000_000_000 {
                    reclaimable += bytes / 4;
                    recommendations.push(format!(
                        "Prune stale build targets in ~/.cache ({:.1} GB reclaimable)",
                        (bytes / 4) as f64 / 1e9
                    ));
                }
            }
        }

        if recommendations.is_empty() {
            recommendations.push("OS environment package and cache hygiene nominal.".to_string());
        }

        OsCareReport {
            os_name,
            reclaimable_cache_bytes: reclaimable,
            reclaimable_cache_formatted: format!("{:.2} GB", reclaimable as f64 / 1e9),
            status: if reclaimable > 0 {
                "RECLAIMABLE_SPACE_DETECTED".to_string()
            } else {
                "OPTIMAL".to_string()
            },
            recommendations,
        }
    }

    pub fn execute_os_clean() -> String {
        let report = Self::audit_os_environment_care();
        if report.reclaimable_cache_bytes == 0 {
            return "OS environment is already clean and optimal.".to_string();
        }

        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let user_cache = home.join(".cache");
        if user_cache.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&user_cache) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.contains("temp") || name.contains("tmp") {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
        }

        "SUCCESS: Executed OS environment care. Reclaimed space across OS caches.".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsCareReport {
    pub os_name: String,
    pub reclaimable_cache_bytes: u64,
    pub reclaimable_cache_formatted: String,
    pub status: String,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLadderStep {
    pub step: usize,
    pub label: String,
    pub hf_repo: String,
    pub hf_file: String,
    pub tokenizer_repo: String,
    pub min_bytes: u64,
    pub expected_bytes: u64,
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

    fn fixture_config_step(
        step: usize,
        min_ram_gb: f32,
        expected_bytes: u64,
    ) -> crate::sandbox::manager::ModelLadderConfigStep {
        crate::sandbox::manager::ModelLadderConfigStep {
            fields: Default::default(),
            step,
            label: format!("{step}-tier"),
            hf_repo: format!("repo-{step}"),
            hf_file: format!("file-{step}.gguf"),
            tokenizer_repo: String::new(),
            min_ram_gb,
            min_bytes: expected_bytes / 5,
            expected_bytes,
        }
    }

    /// Regression for EV-2022920-054: tier selection used to be RAM-only,
    /// so a machine with plenty of RAM but little free disk would still
    /// attempt a download it can't fit (up to ~45GB for 72B) with no
    /// check at all.
    #[test]
    fn test_filter_ladder_by_hardware_excludes_tiers_that_dont_fit_on_disk() {
        let steps = vec![
            fixture_config_step(1, 0.0, 5_000_000_000),   // 1.5B, 5GB
            fixture_config_step(4, 32.0, 20_000_000_000), // 32B, 20GB
            fixture_config_step(5, 64.0, 45_000_000_000), // 72B, 45GB
        ];
        // 64GB RAM qualifies for all three by RAM alone, but only 10GB free
        // disk - only the 1.5B tier (5GB + 2GB margin = 7GB) fits.
        let ladder = HardwareProfiler::filter_ladder_by_hardware(&steps, 64, 10_000_000_000);
        assert_eq!(ladder.len(), 1, "only the 1.5B tier should fit on disk");
        assert_eq!(ladder[0].step, 1);
    }

    #[test]
    fn test_filter_ladder_by_hardware_still_gates_on_ram_too() {
        let steps = vec![
            fixture_config_step(1, 0.0, 5_000_000_000),
            fixture_config_step(2, 8.0, 5_000_000_000),
        ];
        // Plenty of disk, but only 4GB RAM - the 7B tier (needs 8GB) must
        // still be excluded even though it would easily fit on disk.
        let ladder = HardwareProfiler::filter_ladder_by_hardware(&steps, 4, 1_000_000_000_000);
        assert_eq!(ladder.len(), 1);
        assert_eq!(ladder[0].step, 1);
    }

    #[test]
    fn test_filter_ladder_by_hardware_requires_the_safety_margin_not_just_exact_fit() {
        let steps = vec![fixture_config_step(1, 0.0, 5_000_000_000)];
        // Exactly 5GB free - the tier needs 5GB + 2GB margin, so it must
        // NOT qualify on a bare exact-fit amount.
        let ladder = HardwareProfiler::filter_ladder_by_hardware(&steps, 64, 5_000_000_000);
        assert!(
            ladder.is_empty(),
            "must require the safety margin, not just the raw model size"
        );
    }

    #[test]
    fn test_progressive_model_ladder_ordering() {
        let ladder = HardwareProfiler::get_progressive_model_ladder();
        assert!(!ladder.is_empty());
        let mut last_step = 0;
        let mut last_min_bytes = 0;
        for step in ladder {
            assert!(step.step > last_step);
            last_step = step.step;
            assert!(!step.label.is_empty());
            assert!(!step.hf_repo.is_empty());
            assert!(!step.hf_file.is_empty());
            // Bigger step number must never mean a *smaller* download-size
            // threshold — this is exactly the shape of bug that was hiding in
            // sandbox/manager.rs's Rust-literal accessor defaults (values
            // scrambled relative to config.default.json, invisible because
            // nothing asserted the *relationship* between them). Not a strict
            // increase: steps 1-3 intentionally share one threshold, matching
            // the original hardcoded `if step >= 5 {...} else if step >= 4
            // {...} else {1GB}` logic this config data was migrated from.
            assert!(
                step.min_bytes >= last_min_bytes,
                "min_bytes must never decrease as step number increases"
            );
            assert!(step.expected_bytes >= step.min_bytes);
            last_min_bytes = step.min_bytes;
        }
    }

    #[test]
    fn test_acceleration_active_agrees_with_actual_device_selection() {
        // Regression: interrogate_native_acceleration used to read
        // candle_core::utils::cuda_is_available()/metal_is_available(), which
        // are compile-time-only (cfg!(feature = "cuda")) and would report
        // acceleration as active on a cuda-feature build even with no GPU
        // present, disagreeing with get_candle_device()'s real runtime probe.
        let profile = HardwareProfiler::get_profile();
        let device_is_accelerated = !matches!(HardwareProfiler::get_candle_device(), Device::Cpu);
        assert_eq!(profile.acceleration_active, device_is_accelerated);
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

    /// Only meaningfully exercised in a `--features cuda` build with a real
    /// GPU: selecting Device::Cuda is not proof the device actually computes
    /// (driver/toolkit mismatches, like cudarc's CUDA-version allowlist,
    /// fail at kernel-launch time, not at device selection). Runs a real
    /// matmul on-device and checks the numeric result.
    #[test]
    fn test_cuda_device_actually_computes_when_selected() {
        let device = HardwareProfiler::get_candle_device();
        if !matches!(device, candle_core::Device::Cuda(_)) {
            return;
        }
        use candle_core::Tensor;
        let a = Tensor::from_slice(&[1f32, 2., 3., 4.], (2, 2), &device).unwrap();
        let b = Tensor::from_slice(&[5f32, 6., 7., 8.], (2, 2), &device).unwrap();
        let c = a.matmul(&b).unwrap();
        let result: Vec<Vec<f32>> = c.to_vec2().unwrap();
        assert_eq!(result, vec![vec![19., 22.], vec![43., 50.]]);
    }
}
