// 100% Rust implementation for autonomous hardware profiling

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use susi_vendor_candle::candle_core;
use susi_vendor_candle::candle_core::Device;

/// Direct Win32 hardware probes (`kernel32.dll`) so Windows builds measure
/// real physical/available RAM, pagefile swap, disk capacity/free space, and
/// system uptime in microseconds without spawning PowerShell or returning
/// hardcoded stubs.
#[cfg(windows)]
#[allow(unsafe_code)]
mod win32_hw {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }

    #[allow(non_snake_case)] // the Win32 spellings are the API contract
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available_to_caller: *mut u64,
            total_number_of_bytes: *mut u64,
            total_number_of_free_bytes: *mut u64,
        ) -> i32;
        fn GetTickCount64() -> u64;
    }

    /// Returns `(total_phys_bytes, avail_phys_bytes, total_page_file_bytes)`.
    pub(super) fn memory_status() -> Option<(u64, u64, u64)> {
        let mut status = MemoryStatusEx {
            dw_length: std::mem::size_of::<MemoryStatusEx>() as u32,
            dw_memory_load: 0,
            ull_total_phys: 0,
            ull_avail_phys: 0,
            ull_total_page_file: 0,
            ull_avail_page_file: 0,
            ull_total_virtual: 0,
            ull_avail_virtual: 0,
            ull_avail_extended_virtual: 0,
        };
        // SAFETY: `status` is a live, writable `MemoryStatusEx` with `dw_length`
        // initialized to its exact struct size as required by Win32.
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 {
            Some((
                status.ull_total_phys,
                status.ull_avail_phys,
                status.ull_total_page_file,
            ))
        } else {
            None
        }
    }

    /// Returns `(free_bytes_available, total_bytes)` for the volume containing
    /// `path` (or its nearest existing ancestor / current directory).
    pub(super) fn disk_space(path: &Path) -> Option<(u64, u64)> {
        let existing = path
            .ancestors()
            .find(|p| !p.as_os_str().is_empty() && p.exists())
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\"));
        let wide: Vec<u16> = existing
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free_avail: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free: u64 = 0;
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 path and the three
        // out-pointers reference live stack `u64` variables.
        if unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_avail,
                &mut total_bytes,
                &mut total_free,
            )
        } != 0
        {
            Some((free_avail, total_bytes))
        } else {
            None
        }
    }

    pub(super) fn uptime_secs() -> u64 {
        // SAFETY: `GetTickCount64` takes no arguments and has no preconditions.
        unsafe { GetTickCount64() / 1000 }
    }
}

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
    /// When the host was actually probed. `#[serde(default)]` keeps
    /// profiles serialized before the field existed loadable.
    #[serde(default)]
    pub observed_unix: u64,
}

impl HardwareProfile {
    /// Serialize for publication: ambiguous zeros become null — an
    /// unmeasured field is unknown, never a fabricated measurement — and
    /// the section carries its capture time and provenance.
    ///
    /// `cpus`, `ram_gb`, `gpu_vram_gb` and `disk_gb` cannot truthfully be
    /// zero on a running host, so zero there means the probe failed.
    /// `cpus` is additionally nulled when the live parallelism probe
    /// fails, because the profile's fallback substitutes a plausible
    /// default that a failed probe cannot be told apart from.
    /// `swap_gb` and `disk_usage_pct` keep real zeros: a host without
    /// swap or with an empty disk measures 0.
    #[must_use]
    pub fn report_json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = v.as_object_mut() {
            for key in ["cpus", "ram_gb", "gpu_vram_gb", "disk_gb"] {
                if obj.get(key).and_then(serde_json::Value::as_u64) == Some(0) {
                    obj.insert(key.to_string(), serde_json::Value::Null);
                }
            }
            if std::thread::available_parallelism().is_err() {
                obj.insert("cpus".to_string(), serde_json::Value::Null);
            }
            obj.insert(
                "provenance".to_string(),
                serde_json::json!({"observed": {"source": "hardware_profiler"}}),
            );
        }
        v
    }
}

pub struct HardwareProfiler;

impl HardwareProfiler {
    pub fn get_profile() -> HardwareProfile {
        static CACHED_PROFILE: OnceLock<HardwareProfile> = OnceLock::new();
        let mut profile = CACHED_PROFILE
            .get_or_init(|| {
                let (cpus, _) = Self::profile();
                let ram_gb = Self::determine_total_ram_gb();
                let gpu_vram_gb = Self::gpu_vram_gb();
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
                    observed_unix: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
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
        } else if cfg!(target_os = "windows") {
            if let Ok(id) = std::env::var("PROCESSOR_IDENTIFIER") {
                let trimmed = id.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
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
        #[cfg(windows)]
        {
            let secs = win32_hw::uptime_secs();
            let hours = secs / 3600;
            let mins = (secs % 3600) / 60;
            format!("up {} hours, {} minutes", hours, mins)
        }
        #[cfg(not(windows))]
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
    #[allow(unsafe_code)]
    fn get_disk_stats() -> (usize, u8) {
        use std::ffi::CString;
        if let Ok(c_path) = CString::new("/") {
            // SAFETY: statvfs is a plain C struct; all-zero is a valid value that statvfs() overwrites.
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a live, writable statvfs.
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

    #[cfg(windows)]
    fn get_disk_stats() -> (usize, u8) {
        if let Some((free_bytes, total_bytes)) = win32_hw::disk_space(std::path::Path::new(".")) {
            let used_bytes = total_bytes.saturating_sub(free_bytes);
            let total_gb = (total_bytes / (1024 * 1024 * 1024)) as usize;
            let usage_pct = if total_bytes > 0 {
                ((used_bytes as f64 / total_bytes as f64) * 100.0) as u8
            } else {
                0
            };
            return (total_gb, usage_pct);
        }
        (256, 0)
    }

    #[cfg(not(any(unix, windows)))]
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
    #[allow(unsafe_code)]
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
        // SAFETY: statvfs is a plain C struct; all-zero is a valid value that statvfs() overwrites.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a live, writable statvfs.
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } == 0 {
            (stat.f_bavail as u64) * (stat.f_frsize as u64)
        } else {
            0
        }
    }

    #[cfg(windows)]
    pub fn get_free_disk_bytes(path: &std::path::Path) -> u64 {
        win32_hw::disk_space(path).map_or(0, |(free, _)| free)
    }

    #[cfg(not(any(unix, windows)))]
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
        caps.push(format!("{}V", env!("CARGO_PKG_VERSION")));
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
        // When the live free query fails, the total is asked afresh, never
        // taken from `gpu_vram_gb`'s in-process memo: a failing or vanished
        // `nvidia-smi` must leave a budget of 0 here, as it always did, not
        // a remembered total presented as free memory.
        let free_bytes = Self::determine_gpu_vram_free_bytes()
            .unwrap_or_else(|| Self::fresh_gpu_vram_gb() as u64 * 1024 * 1024 * 1024);
        free_bytes.saturating_sub(CUDA_CONTEXT_RESERVE_BYTES)
    }

    /// Live free VRAM in bytes via `nvidia-smi`, independent of
    /// `gpu_vram_gb`'s integer-GB display value (which truncates,
    /// e.g. an 8188 MiB card reports as "7GB") and independent of total
    /// capacity, which doesn't reflect memory already in use.
    fn determine_gpu_vram_free_bytes() -> Option<u64> {
        // Bounded: nvidia-smi can hang indefinitely on a wedged driver.
        let output = crate::susi_core::bounded_cmd::output_within(
            std::process::Command::new("nvidia-smi")
                .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"]),
            std::time::Duration::from_secs(5),
        )
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
            let vram_limit = Self::gpu_vram_gb() * 1024 * 1024 * 1024;
            if vram_limit > 0 && allocated_bytes_required as f64 > (vram_limit as f64 * 0.98) {
                return Device::Cpu;
            }
        }

        susi_vendor_candle::device::cached_device()
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

    /// Total GPU memory in GB: `nvidia-smi`'s total, else (no total from it)
    /// the AMD sysfs attribute, else 0.
    ///
    /// A total from `nvidia-smi` is kept for the rest of the process (never
    /// between processes, see [`gpu_probe_cache`]): the probe is a spawn that
    /// took about two seconds on a Windows host. The trade-off: a
    /// long-lived process (the daemon's [`Self::get_dynamic_device`], which
    /// used to ask on every call) keeps a total for its whole life, even if
    /// the card goes away under it — the profile's `OnceLock` already did.
    /// Nothing else is kept: with no `nvidia-smi`, or one that gave no total
    /// (timed out, failed, could not start), every call asks again — a
    /// daemon sees a driver installed after it started, and one
    /// `nvidia-smi` timeout at its start does not leave its `vram_limit` at
    /// 0 for its whole life.
    fn gpu_vram_gb() -> usize {
        static TOTAL_GB: OnceLock<usize> = OnceLock::new();
        gpu_probe_cache::memoized(&TOTAL_GB, nvidia_smi_total_probe, Self::sysfs_gpu_vram_gb)
    }

    /// [`Self::gpu_vram_gb`]'s answer without its memo: `nvidia-smi` is asked
    /// now. Used where a failure of the live query must not be answered from
    /// memory ([`Self::gpu_vram_budget_bytes`]).
    fn fresh_gpu_vram_gb() -> usize {
        nvidia_smi_total_probe().unwrap_or_else(Self::sysfs_gpu_vram_gb)
    }

    /// The AMD driver's total VRAM (Linux sysfs), else 0. NVIDIA's
    /// proprietary driver never populates this attribute, which is why
    /// `nvidia-smi` is asked first.
    fn sysfs_gpu_vram_gb() -> usize {
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
                                return Self::container_available_kb(kb) / (1024 * 1024);
                            }
                        }
                    }
                }
            }
        }
        #[cfg(windows)]
        if let Some((total_phys, _, total_page)) = win32_hw::memory_status() {
            return (total_page.saturating_sub(total_phys) / (1024 * 1024 * 1024)) as usize;
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

    #[allow(unsafe_code)]
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
                    // SAFETY: `c_name` is NUL-terminated and `val`/`size` describe a live u64 buffer of the stated size.
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
            #[cfg(windows)]
            if let Some((total_phys, _, _)) = win32_hw::memory_status() {
                let gb = (total_phys / (1024 * 1024 * 1024)) as usize;
                if gb > 0 {
                    return gb;
                }
            }
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
        if cfg!(target_os = "macos") {
            if let Ok(output) = std::process::Command::new("vm_stat").output() {
                let text = String::from_utf8_lossy(&output.stdout);
                let page_size = text
                    .lines()
                    .next()
                    .and_then(|line| line.split("page size of ").nth(1))
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(4096);
                let pages: u64 = text
                    .lines()
                    .filter(|l| {
                        l.starts_with("Pages free:")
                            || l.starts_with("Pages inactive:")
                            || l.starts_with("Pages speculative:")
                    })
                    .filter_map(|l| {
                        l.split(':')
                            .nth(1)?
                            .trim()
                            .trim_end_matches('.')
                            .parse::<u64>()
                            .ok()
                    })
                    .sum();
                return (pages.saturating_mul(page_size) / 1073741824) as usize;
            }
        }
        #[cfg(windows)]
        if let Some((_, avail_phys, _)) = win32_hw::memory_status() {
            return (avail_phys / (1024 * 1024 * 1024)) as usize;
        }
        0 // Unknown available memory must not authorize automatic downloads.
    }

    fn container_available_kb(host_kb: usize) -> usize {
        let Ok(groups) = std::fs::read_to_string("/proc/self/cgroup") else {
            return host_kb;
        };
        let Some(relative) = groups.lines().find_map(|l| l.strip_prefix("0::")) else {
            return host_kb;
        };
        let root = std::path::Path::new("/sys/fs/cgroup");
        let path = root.join(relative.trim_start_matches('/'));
        let mut available = host_kb;
        // Parent cgroups can impose a tighter limit than the leaf.
        for group in path.ancestors().take_while(|p| p.starts_with(root)) {
            let maximum = std::fs::read_to_string(group.join("memory.max"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());
            let current = std::fs::read_to_string(group.join("memory.current"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());
            if let (Some(maximum), Some(current)) = (maximum, current) {
                available = available.min((maximum.saturating_sub(current) / 1024) as usize);
            }
        }
        available
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
        steps: &[crate::susi_sandbox::manager::ModelLadderConfigStep],
        ram_gb: usize,
        free_disk_bytes: u64,
    ) -> Vec<crate::susi_sandbox::manager::ModelLadderConfigStep> {
        const DISK_SAFETY_MARGIN_BYTES: u64 = 2_000_000_000; // 2GB headroom
        steps
            .iter()
            .filter(|s| {
                ram_gb > 0
                    && ram_gb as f32 >= s.min_ram_gb
                    && free_disk_bytes >= s.expected_bytes.saturating_add(DISK_SAFETY_MARGIN_BYTES)
            })
            .cloned()
            .collect()
    }

    fn admission_ram_gb(available_gb: usize, reserve_gb: f32) -> usize {
        if !reserve_gb.is_finite() {
            return 0;
        }
        (available_gb as f32 - reserve_gb.max(0.0)).max(0.0).floor() as usize
    }

    pub fn get_progressive_model_ladder() -> Vec<ModelLadderStep> {
        // Model admission must reflect memory that is actually available to
        // this process, not RAM installed in the machine. The latter can
        // qualify a model that will immediately OOM on a busy host.
        let ram_gb = Self::determine_available_ram_gb();
        let free_disk_bytes = Self::get_free_disk_bytes(&crate::ModelManager::get_models_dir());
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();

        let config_steps = crate::hf_discovery::resolve_model_ladder(&cfg);
        let reserve = cfg.model_scoring_heuristics().system_ram_buffer_gb;
        let budget = Self::admission_ram_gb(ram_gb, reserve);
        let models_dir = crate::ModelManager::get_models_dir();
        let qualifying: Vec<_> = config_steps
            .into_iter()
            .filter(|step| {
                let path = models_dir.join(&step.hf_file);
                let mut admission = step.clone();
                let resident = crate::ModelManager::valid_gguf_payload(&path);
                let partial = std::path::PathBuf::from(format!("{}.part", path.display()))
                    .metadata()
                    .map(|m| m.len())
                    .unwrap_or(0);
                admission.expected_bytes = if resident {
                    0
                } else {
                    step.expected_bytes.saturating_sub(partial)
                };
                // Configured tiers cannot understate the minimum weight footprint.
                admission.min_ram_gb = admission.min_ram_gb.max(
                    step.expected_bytes as f32 / 1073741824.0
                        * cfg.model_lifecycle().memory_overhead_ratio.max(1.0),
                );
                !Self::filter_ladder_by_hardware(&[admission], budget, free_disk_bytes).is_empty()
            })
            .collect();
        let ladder: Vec<ModelLadderStep> = qualifying
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

        ladder
    }

    pub fn audit_os_environment_care() -> OsCareReport {
        let os_name = Self::get_os_info();
        let mut reclaimable = 0u64;
        let mut recommendations = Vec::new();

        // Host package cache: advice only. It is the host's, not SUSI's —
        // `susi os-clean` never touches it, so it is not "reclaimable" here.
        let pacman_cache = std::path::PathBuf::from("/var/cache/pacman/pkg");
        if pacman_cache.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&pacman_cache) {
                let bytes: u64 = entries
                    .flatten()
                    .filter_map(|e| e.metadata().ok())
                    .filter(std::fs::Metadata::is_file)
                    .map(|m| m.len())
                    .sum();
                if bytes > 500_000_000 {
                    recommendations.push(format!(
                        "Host pacman package cache holds {:.1} GB (clean with `paccache -r`; susi leaves host caches alone)",
                        bytes as f64 / 1e9
                    ));
                }
            }
        }

        // Rotated metrics generations beyond the sink's rotation cap are
        // pre-cap residue — current rotation can never produce them.
        let rotated = susi_paths::SusiDirs::data_dir().join("error_metrics.jsonl.1");
        if let Ok(meta) = std::fs::metadata(&rotated) {
            if meta.len() > crate::susi_error::sink::METRICS_CAP_BYTES {
                reclaimable += meta.len();
                recommendations.push(format!(
                    "Remove pre-cap rotated metrics ({:.1} GB reclaimable)",
                    meta.len() as f64 / 1e9
                ));
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
                "NOTHING_RECLAIMABLE".to_string()
            },
            recommendations,
        }
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

/// `nvidia-smi --query-gpu=memory.total`: the first GPU's total in GB, or
/// `None` when it gave no total — absent, failed, timed out, or could not
/// start.
fn nvidia_smi_total_probe() -> Option<usize> {
    nvidia_smi_total_from(crate::susi_core::bounded_cmd::output_within(
        std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"]),
        std::time::Duration::from_secs(5),
    ))
}

/// The total an `nvidia-smi` run answered: a positive MiB count on its first
/// line of a successful run.
fn nvidia_smi_total_from(answer: std::io::Result<std::process::Output>) -> Option<usize> {
    let output = answer.ok().filter(|output| output.status.success())?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|line| line.trim().parse::<u64>().ok())
        .filter(|mib| *mib > 0)
        .map(|mib| (mib / 1024) as usize)
}

/// Keeping `nvidia-smi`'s GPU total in the process
/// ([`HardwareProfiler::gpu_vram_gb`]). Nothing is kept between processes:
/// a card can go away without a reboot (an eGPU unplugged, the adapter
/// disabled, a driver that fails to load) while every cheap fact about the
/// host stays the same, so only a fresh `nvidia-smi` run can say a total
/// still holds.
mod gpu_probe_cache {
    /// `kept`'s total, else `probe`'s, kept for the rest of the process once
    /// there is one; without one, `fallback` answers this call and the next
    /// call probes again.
    pub(super) fn memoized(
        kept: &std::sync::OnceLock<usize>,
        probe: impl FnOnce() -> Option<usize>,
        fallback: impl FnOnce() -> usize,
    ) -> usize {
        if let Some(gb) = kept.get() {
            return *gb;
        }
        match probe() {
            // A concurrent first call may have kept its own total.
            Some(gb) => *kept.get_or_init(|| gb),
            None => fallback(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Only a total is kept. No `nvidia-smi`, or one that gave no total,
        /// is asked again by the next call (a daemon sees a driver installed
        /// after it started); the fallback answers meanwhile.
        #[test]
        fn only_a_total_is_ever_kept() {
            let kept = std::sync::OnceLock::new();
            let probes = std::cell::Cell::new(0);
            let probe = |found: Option<usize>| {
                probes.set(probes.get() + 1);
                found
            };
            assert_eq!(memoized(&kept, || probe(None), || 3), 3);
            assert_eq!(memoized(&kept, || probe(None), || 3), 3);
            assert_eq!(probes.get(), 2, "no total: every call asks again");
            assert_eq!(memoized(&kept, || probe(Some(24)), || 3), 24);
            assert_eq!(memoized(&kept, || probe(None), || 3), 24);
            assert_eq!(probes.get(), 3, "a total is kept");
        }

        /// What counts as a total: a positive MiB count on the first line of
        /// a run that succeeded. No program, a timeout, or a failed run is
        /// no total.
        #[test]
        fn a_total_is_a_positive_count_from_a_run_that_succeeded() {
            use super::super::nvidia_smi_total_from;
            use std::io::{Error, ErrorKind};
            let echo = |text: &str| {
                if cfg!(windows) {
                    std::process::Command::new("cmd")
                        .args(["/d", "/c", &format!("echo {text}")])
                        .output()
                } else {
                    std::process::Command::new("/bin/sh")
                        .args(["-c", &format!("echo {text}")])
                        .output()
                }
            };
            assert_eq!(nvidia_smi_total_from(echo("8188")), Some(7));
            assert_eq!(nvidia_smi_total_from(echo("512")), Some(0));
            assert_eq!(nvidia_smi_total_from(echo("0")), None);
            assert_eq!(nvidia_smi_total_from(echo("No devices were found")), None);
            assert_eq!(
                nvidia_smi_total_from(Err(Error::from(ErrorKind::NotFound))),
                None
            );
            assert_eq!(
                nvidia_smi_total_from(Err(Error::from(ErrorKind::TimedOut))),
                None
            );
            let failed = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--susi-no-such-flag")
                .output();
            assert!(failed.as_ref().is_ok_and(|out| !out.status.success()));
            assert_eq!(nvidia_smi_total_from(failed), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hardware_profile_generation() {
        let profile = HardwareProfiler::get_profile();
        assert!(profile.cpus > 0);
        assert!(profile.ram_gb > 0);
        assert!(profile.observed_unix > 0);
    }

    #[test]
    fn vc_201_041_mastery_report_json_never_publishes_fabricated_zeros() {
        let zeroed = HardwareProfile {
            cpus: 0,
            cpu_brand: String::new(),
            gpu_info: "none".into(),
            ram_gb: 0,
            available_ram_gb: 0,
            gpu_vram_gb: 0,
            swap_gb: 0,
            nvme_active: false,
            acceleration_active: false,
            native_acceleration: "None".into(),
            os_info: "test".into(),
            arch: "x86_64".into(),
            disk_gb: 0,
            disk_usage_pct: 0,
            load_avg: String::new(),
            uptime: String::new(),
            hostname: String::new(),
            observed_unix: 1_700_000_000,
        };
        let v = zeroed.report_json();
        // A field the probe could not measure publishes null — never a
        // fabricated measurement.
        for key in ["cpus", "ram_gb", "gpu_vram_gb", "disk_gb"] {
            assert!(v.get(key).is_some_and(serde_json::Value::is_null), "{key}");
        }
        // Zero is a real measurement for swap and disk usage — kept.
        assert_eq!(
            v.get("swap_gb").and_then(serde_json::Value::as_u64),
            Some(0)
        );
        assert_eq!(
            v.get("disk_usage_pct").and_then(serde_json::Value::as_u64),
            Some(0)
        );
        // The section carries its observation time and provenance.
        assert_eq!(
            v.get("observed_unix").and_then(serde_json::Value::as_u64),
            Some(1_700_000_000)
        );
        assert!(v.get("provenance").is_some());

        let measured = HardwareProfile {
            cpus: 8,
            ram_gb: 32,
            gpu_vram_gb: 24,
            disk_gb: 512,
            ..zeroed
        };
        let v = measured.report_json();
        for (key, want) in [
            ("cpus", 8),
            ("ram_gb", 32),
            ("gpu_vram_gb", 24),
            ("disk_gb", 512),
        ] {
            assert_eq!(
                v.get(key).and_then(serde_json::Value::as_u64),
                Some(want),
                "{key}"
            );
        }
    }

    fn fixture_config_step(
        step: usize,
        min_ram_gb: f32,
        expected_bytes: u64,
    ) -> crate::susi_sandbox::manager::ModelLadderConfigStep {
        crate::susi_sandbox::manager::ModelLadderConfigStep {
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
    fn test_admission_reserves_available_memory() {
        let steps = vec![
            fixture_config_step(1, 4.0, 2_000_000_000),
            fixture_config_step(4, 32.0, 20_000_000_000),
        ];
        let budget = HardwareProfiler::admission_ram_gb(5, 2.0);
        assert!(HardwareProfiler::filter_ladder_by_hardware(&steps, budget, u64::MAX).is_empty());
        assert_eq!(HardwareProfiler::admission_ram_gb(5, 1.0), 4);
        assert_eq!(HardwareProfiler::admission_ram_gb(1, 2.0), 0);
        assert_eq!(HardwareProfiler::admission_ram_gb(64, f32::NAN), 0);
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

    #[test]
    fn test_native_host_memory_and_disk_telemetry_are_live() {
        let total_ram = HardwareProfiler::determine_total_ram_gb();
        let avail_ram = HardwareProfiler::determine_available_ram_gb();
        assert!(total_ram > 0, "total RAM must be non-zero on a live host");
        assert!(
            avail_ram <= total_ram,
            "available RAM ({avail_ram} GB) cannot exceed total RAM ({total_ram} GB)"
        );
        let free_disk = HardwareProfiler::get_free_disk_bytes(std::path::Path::new("."));
        assert!(
            free_disk > 0 && free_disk < u64::MAX,
            "get_free_disk_bytes must return a real filesystem measurement, got {free_disk}"
        );
        let (disk_gb, usage_pct) = HardwareProfiler::get_disk_stats();
        assert!(disk_gb > 0, "disk capacity must be non-zero");
        assert!(usage_pct <= 100, "disk usage pct must be <= 100");
    }
}
